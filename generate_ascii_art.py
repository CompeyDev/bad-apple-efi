#!/bin/env python3

"""
generate_ascii_art.py

Source: https://github.com/Chion82/ASCII_bad_apple/blob/master/generate_ascii_art.py
Authors:
    - Chion82 (https://github.com/Chion82): Original author of the script
    - Erica Marigold (https://github.com/CompeyDev): Modified for python3 support
"""

from PIL import Image

video_length = 100

ASCII_CHARS = '$@B%8&WM#*oahkbdpqwmZO0QLCJUYXzcvunxrjft/\|()1{}[]?-_+~<>i!lI;:,"^`\'. '

def scale_image(image, new_width=100, new_height=30):
    """Resizes an image preserving the aspect ratio.
    """
    (original_width, original_height) = image.size
    aspect_ratio = original_height/float(original_width)
    if new_height == 0:
        new_height = int(aspect_ratio * new_width)

    new_image = image.resize((new_width, new_height))
    return new_image

def convert_to_grayscale(image):
    return image.convert('L')

def map_pixels_to_ascii_chars(image, range_width=3.69):
    """Maps each pixel to an ascii char based on the range
    in which it lies.

    0-255 is divided into 11 ranges of 25 pixels each.
    """

    pixels_in_image = list(image.getdata())
    pixels_to_chars = [ASCII_CHARS[int(pixel_value/range_width)] for pixel_value in
            pixels_in_image]

    return "".join(pixels_to_chars)

def convert_image_to_ascii(image, new_width=300, new_height=90):
    image = scale_image(image, new_width, new_height)
    image = convert_to_grayscale(image)

    pixels_to_chars = map_pixels_to_ascii_chars(image)
    len_pixels_to_chars = len(pixels_to_chars)

    image_ascii = [pixels_to_chars[index: index + new_width] for index in
            range(0, len_pixels_to_chars, new_width)]

    return "\n".join(image_ascii)

def handle_image_conversion(image_filepath):
    image = Image.open(image_filepath)

    return convert_image_to_ascii(image)

if __name__=='__main__':
    import os
    import time 
    import cv2

    vidcap = cv2.VideoCapture('bin/bad_apple.mp4')
    time_count = 0
    f = open('ascii.txt', 'a')

    while time_count <= video_length * 1000:
        print('Generating ASCII frame at ' + str(time_count))
        vidcap.set(0, time_count)
        success, image = vidcap.read()
        if success:
            cv2.imwrite('extracted.jpg', image)

        f.write(handle_image_conversion('extracted.jpg'))
        f.write('SPLIT')

        time_count = time_count + 100

    f.close()
    os.remove('extracted.jpg')
